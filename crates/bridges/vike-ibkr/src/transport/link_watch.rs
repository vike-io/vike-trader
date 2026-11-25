//! **The link watchdog**: does IB Gateway still have a link to IBKR's servers? (2026-10-17: it lost it at 13:36
//! and 17:00; the daemon only probed the PORT, which kept answering, so the adapter sat on a dead link for ever.)
//!
//! `ibapi` (4.2.0, and 5.1.0 and 5.2.0 the same) delivers the connectivity notices (1100/1101/1102, farm status 2103..2158, 2110) ONLY to the
//! client's `NoticeStream`, never to the order-update stream the pump reads; a connection's handshake burst
//! carries the CURRENT state, broken parts included, which only a stream bound BEFORE the connect sees
//! (`ClientBuilder::connect_with_notice_stream`). The real the CI box journal of that day (`ibapi::transport::common`):
//! a gateway without its link announces `[2110]` (server link), `[2103] ...:usfarm` and `[2157] ...:secdefil` on
//! every new connection and nothing restores them; a healthy one announces `[2104]`, `[2106]`, `[2158]` OK.
//!
//! [`LinkMonitor`] keeps the set of broken parts, each with the time it broke:
//!
//! | part | broken by | cleared by | counts toward LOST |
//! |---|---|---|---|
//! | server link | `1100`, `2110` | `1101`, `1102`, the OK of a farm that was broken, a socket reconnect (`-8`) | yes |
//! | `ibapi` socket | the client reports itself not connected | it reports connected | yes |
//! | snapshot | a snapshot request ended incomplete (the venue did not answer) | a snapshot that completes | yes |
//! | market data farm | `2103` | `2104` of that farm | NO |
//! | sec-def farm | `2157` | `2158` of that farm | NO |
//!
//! **A farm alone is not a lost link.** the CI box's journal shows `[2103] usfarm` broken for 136 s with the server link
//! fine and no 1100/2110; the farm keys are kept only so that the OK of a farm we saw broken can clear the server
//! link (a farm cannot be OK without it). **90 s was inside the normal range**: a nightly-window outage ran 1100 at
//! 04:24:18 to 1102 at 04:25:52 (94 s) and healed by itself, others lasted 27 s and 31 s, while the incident that
//! made this watchdog lasted hours; so the threshold is [`LOST_AFTER`], 5 minutes.
//!
//! `2105`/`2106`/`2107` (the history farm) are not tracked: they say nothing about orders, and `2107` (inactive) is
//! a normal idle state. [`Watch`] is what the pump drives: it drains the notices, asks the monitor, tells the
//! [`GatewayGate`] (which then spaces the rebuild) and returns the reason the pump turns into
//! `IbkrInbound::StreamDead`. No health request is sent: the gateway answers `reqCurrentTime` from its own clock
//! (unproven here), and a blocked probe thread would hold the client id this watchdog exists to free.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use vike_bridge_core::venue_link::{VenueLink, VenueLinkDown, VenueLinkState};
use vike_venues::venue_timings;

use super::gateway_gate::GatewayGate;

/// How old the watch's last "up" may be and still vouch for the path (the account's `VenueLink`'s freshness bound):
/// the pump steps its watch at least every 250 ms (`IDLE_POLL`), so 5 s without a step is a pump that is stuck or
/// gone, not a quiet stream.
const FRESH: Duration = Duration::from_secs(5);

/// The link cell's word for a path that is broken and not lost yet: `why` in the watch's words, never a value; `effect`
/// what it costs the orders. No failure is counted: IB Gateway heals most of these by itself inside [`LOST_AFTER`] (the
/// nightly reset's 94 s), so the reading is a reopen (`Connecting`), said once it outlives an ask.
fn link_down(why: String, effect: &'static str) -> VenueLinkState {
    VenueLinkState::Down(VenueLinkDown {
        why,
        effect,
        failed_restarts: 0,
        retry_in: Duration::ZERO,
    })
}

/// The link cell's word for a connection that is DEAD (lost for [`LOST_AFTER`], or its order-update stream ended): a
/// failure (`Error`), said at once. No restart is counted and no wait stated here: the gatewayhub rebuilds the connection
/// on its own schedule.
fn link_lost(why: String) -> VenueLinkState {
    VenueLinkState::Down(VenueLinkDown::failed(why, DEAD_EFFECT, 0, Duration::ZERO))
}

/// How long a counted part may stay broken before the link is called lost (the module doc has the data): the exec
/// lane's `lost_after` in `vike_venues::venue_timings::IBKR`, which carries the reason.
pub(crate) const LOST_AFTER: Duration = venue_timings::IBKR.exec().lost_after();

/// How long a session must run with nothing counted broken before the gate forgets the outage: longer than
/// [`LOST_AFTER`], or a session would be forgiven just before it could be judged. The exec lane's `probation` in
/// `vike_venues::venue_timings::IBKR` (the row's sanity check refuses a probation that does not outlast `lost_after`).
pub(crate) const PROBATION: Duration = venue_timings::IBKR.exec().probation();

const SERVER: &str = "server";
const SOCKET: &str = "socket";
const SNAPSHOT: &str = "snapshot";
const FARM: &str = "farm:";
const SECDEF: &str = "secdef:";

/// The global notices of the `ibapi` client, drained without blocking. `(code, message)`.
pub(crate) trait Notices: Send {
    fn next(&mut self) -> Option<(i32, String)>;
}

/// The broken parts of a gateway's link. See the module doc.
#[derive(Debug, Default)]
pub(crate) struct LinkMonitor {
    broken: BTreeMap<String, Instant>,
}

/// A link that has been broken for too long.
#[derive(Debug)]
pub(crate) struct VenueLinkWatchOutage {
    /// When the earliest still-broken part broke.
    pub(crate) began: Instant,
    pub(crate) age: Duration,
    /// The broken parts, named.
    pub(crate) parts: Vec<String>,
}

impl VenueLinkWatchOutage {
    /// The reason carried by the stream death: what the operator reads in the journal and in a refused order.
    pub(crate) fn reason(&self) -> String {
        format!(
            "IB Gateway has no link to IBKR for {} s ({}); this connection is closed to orders and the session is \
             rebuilt on a spaced schedule",
            self.age.as_secs(),
            self.parts.join("; ")
        )
    }
}

fn describe(key: &str) -> String {
    if key == SERVER {
        "connectivity broken (1100/2110) and not restored (1101/1102)".to_string()
    } else if key == SOCKET {
        "the API socket is not connected".to_string()
    } else if key == SNAPSHOT {
        "the gateway did not answer a snapshot request".to_string()
    } else if let Some(farm) = key.strip_prefix(FARM) {
        format!("market data farm {farm} broken (2103) and not restored (2104)")
    } else if let Some(farm) = key.strip_prefix(SECDEF) {
        format!("sec-def farm {farm} broken (2157) and not restored (2158)")
    } else {
        key.to_string()
    }
}

impl LinkMonitor {
    /// Fold one global notice.
    pub(crate) fn notice(&mut self, code: i32, message: &str, now: Instant) {
        // The farm is named after the last colon (`...is broken:usfarm`); a message without one names none.
        let name = message.rsplit_once(':').map_or("", |(_, n)| n.trim()).to_ascii_lowercase();
        match code {
            1100 | 2110 => self.set(SERVER, true, now),
            1101 | 1102 => self.set(SERVER, false, now),
            // After a socket reconnect a recorded 1100 can never learn the link is back (`ibapi` does not replay
            // restoration notices on the new connection): start from a clean slate.
            c if c == ibapi::TRANSPORT_RECONNECT_CODE => self.broken.clear(),
            2103 => self.set(&format!("{FARM}{name}"), true, now),
            2157 => self.set(&format!("{SECDEF}{name}"), true, now),
            2104 => self.restored(FARM, &name, now),
            2158 => self.restored(SECDEF, &name, now),
            _ => {}
        }
    }

    /// A farm we saw broken is OK again: that farm is clear and so is the server link (a farm cannot be OK
    /// without it). An OK for a farm we never saw broken says nothing (every healthy handshake sends them).
    fn restored(&mut self, prefix: &str, name: &str, now: Instant) {
        let before = self.broken.len();
        self.broken.retain(|k, _| {
            !(k.starts_with(prefix) && (name.is_empty() || k[prefix.len()..] == *name))
        });
        if self.broken.len() != before {
            self.set(SERVER, false, now);
        }
    }

    /// Mark one part broken (kept at the time it FIRST broke) or clear it.
    pub(crate) fn set(&mut self, part: &str, broken: bool, now: Instant) {
        if broken {
            self.broken.entry(part.to_string()).or_insert(now);
        } else {
            self.broken.remove(part);
        }
    }

    /// The parts that count toward LOST: the server link, the socket and a snapshot. A farm does not.
    fn counted(&self) -> impl Iterator<Item = (&String, &Instant)> {
        self.broken.iter().filter(|(k, _)| matches!(k.as_str(), SERVER | SOCKET | SNAPSHOT))
    }

    /// Whether no counted part is broken.
    pub(crate) fn is_clear(&self) -> bool {
        self.counted().next().is_none()
    }

    /// Whether the gateway says it has no link to IBKR's servers right now (`1100`/`2110`, no restore yet).
    fn server_link_broken(&self) -> bool {
        self.broken.contains_key(SERVER)
    }

    /// The outage, once the earliest broken counted part has been broken for `after`.
    pub(crate) fn lost(&self, now: Instant, after: Duration) -> Option<VenueLinkWatchOutage> {
        let began = *self.counted().map(|(_, t)| t).min()?;
        let age = now.saturating_duration_since(began);
        (age >= after).then(|| VenueLinkWatchOutage {
            began,
            age,
            parts: self.counted().map(|(k, _)| describe(k)).collect(),
        })
    }
}

/// The two waits of the watchdog; a test shortens them.
#[derive(Clone, Copy)]
pub(crate) struct WatchTimes {
    pub(crate) lost_after: Duration,
    pub(crate) probation: Duration,
}

impl WatchTimes {
    pub(crate) const PRODUCTION: Self = Self { lost_after: LOST_AFTER, probation: PROBATION };
}

/// What a transport is started with to be watched: the gate of its gateway, the waits, and the account's
/// [`VenueLink`] the watch states its reading to (`None`: nothing is reported).
pub(crate) struct Watching {
    pub(crate) gate: Arc<GatewayGate>,
    pub(crate) times: WatchTimes,
    pub(crate) link: Option<VenueLink>,
}

/// What a broken server link costs the orders: `crate::execution`'s submit gate refuses every one (`NO_SERVER_LINK`).
const REFUSED_EFFECT: &str = "every order is refused, not sent, until IB Gateway says its link to IBKR is back \
                              (1101/1102); after 5 minutes the bridge closes this connection";

/// What a down socket or an unanswered snapshot costs: nothing here refuses the order, and nothing proves it arrives.
const UNSURE_EFFECT: &str = "orders are not refused here but may not reach IBKR: one the socket cannot write comes \
                             back rejected, one the gateway never answers is ended by the stuck-order ladder; after \
                             5 minutes the bridge closes this connection";

/// What a dead order-update stream costs: `crate::execution`'s submit gate refuses every order on this connection.
const DEAD_EFFECT: &str = "every order is refused until this connection is built again";

/// The watchdog of one transport, driven by its pump.
pub(crate) struct Watch {
    monitor: LinkMonitor,
    notices: Option<Box<dyn Notices>>,
    gate: Arc<GatewayGate>,
    times: WatchTimes,
    clear_since: Option<Instant>,
    healed: bool,
    /// Whether any snapshot of this session has completed.
    answered: bool,
    /// The monitor's word on the server link, for the exec thread's submit gate ([`Self::server_link_flag`]).
    server_down: Arc<AtomicBool>,
    /// Where [`Self::step`] states [`Self::reading`] for the gatewayhub's status verb (the account's one cell,
    /// `vike_bridge_core::venue_link::venue_link` by its route key).
    link: Option<VenueLink>,
}

impl Watch {
    pub(crate) fn new(setup: Watching, notices: Option<Box<dyn Notices>>, now: Instant) -> Self {
        Self {
            monitor: LinkMonitor::default(),
            notices,
            gate: setup.gate,
            times: setup.times,
            clear_since: Some(now),
            healed: false,
            answered: false,
            server_down: Arc::new(AtomicBool::new(false)),
            link: setup.link,
        }
    }

    /// Raised while the gateway has no link to IBKR's servers, read by [`super::IbkrTransport::server_link_broken`]:
    /// the pump owns this watch, and the exec thread asks before it writes an order.
    pub(crate) fn server_link_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.server_down)
    }

    /// A snapshot request ended: a completed one clears the part, an incomplete one breaks it, EXCEPT once the
    /// session has had a snapshot answered: the gateway does answer, so one incomplete reply (the stranded-fill
    /// sweep's `request_executions`, say) is not a dead link (the learning of a gateway with no link never gets
    /// a first answer, which is what this part is for).
    pub(crate) fn snapshot(&mut self, complete: bool, now: Instant) {
        if complete {
            self.answered = true;
        } else if self.answered {
            return;
        }
        self.monitor.set(SNAPSHOT, !complete, now);
    }

    /// One pass: fold the notices, read the socket's state, and judge. `Some(reason)` = the link is lost (the gate
    /// has been told); `None` otherwise. After `probation` of nothing broken the gate forgets a past outage. Then the
    /// reading goes to the account's link: the loss itself, or [`Self::reading`]. Writing it judges nothing.
    pub(crate) fn step(&mut self, connected: bool, now: Instant) -> Option<String> {
        let lost = self.judge(connected, now);
        match &lost {
            Some(reason) => self.ended(reason),
            None => {
                if let Some(link) = &self.link {
                    link.set_at(self.reading(), Some(FRESH), now);
                }
            }
        }
        lost
    }

    /// What this watch says of the path now, changing nothing it judges: a COUNTED part broken is down, in
    /// [`describe`]'s words (a farm alone is not); nothing broken once the gateway has answered a snapshot is up; nothing
    /// broken and nothing answered yet is not known.
    pub(crate) fn reading(&self) -> VenueLinkState {
        let parts: Vec<String> = self.monitor.counted().map(|(k, _)| describe(k)).collect();
        if !parts.is_empty() {
            let effect =
                if self.monitor.server_link_broken() { REFUSED_EFFECT } else { UNSURE_EFFECT };
            return link_down(parts.join("; "), effect);
        }
        if self.answered { VenueLinkState::Up } else { VenueLinkState::Unknown }
    }

    /// The connection is dead (the watch declared it lost, or the order-update stream ended): down until a new
    /// connection starts (`crate::connect_execution_client` resets the link).
    pub(crate) fn ended(&self, why: &str) {
        if let Some(link) = &self.link {
            link.set(link_lost(why.to_string()), None);
        }
    }

    /// [`Self::step`]'s judgement, unchanged.
    fn judge(&mut self, connected: bool, now: Instant) -> Option<String> {
        while let Some((code, message)) = self.notices.as_mut().and_then(|n| n.next()) {
            self.monitor.notice(code, &message, now);
        }
        self.server_down.store(self.monitor.server_link_broken(), Ordering::Relaxed);
        self.monitor.set(SOCKET, !connected, now);
        if let Some(outage) = self.monitor.lost(now, self.times.lost_after) {
            self.gate.lost(outage.began);
            return Some(outage.reason());
        }
        if self.monitor.is_clear() {
            let since = *self.clear_since.get_or_insert(now);
            if !self.healed && now.saturating_duration_since(since) >= self.times.probation {
                self.healed = true;
                self.gate.healthy();
            }
        } else {
            self.clear_since = None;
        }
        None
    }
}

#[cfg(test)]
#[path = "link_watch_tests.rs"]
mod link_watch_tests;
