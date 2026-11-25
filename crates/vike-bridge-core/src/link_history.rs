//! **The ONE call a venue session reports through**, so `link_session` (`vike_secrets::link_sessions`) holds every venue
//! in one table and no bridge knows there is a table.
//!
//! The shared user-data pump calls it from its status cell (`websocket_session::user::stream_link`), and a bridge that keeps its own
//! session calls it where that session comes up and ends. [`VenueLinkRecord`] is the shape: opened now, [`VenueLinkRecord::ended`]
//! with the venue's words, and dropped without that, ended by us.
//!
//! ⚠ **A report never blocks and never fails a venue thread.** It is a `try_send` onto a bounded channel to one writer
//! thread (`crates/vike-gatewayhub/src/link_writer.rs`, which alone installs [`install`]'s sink); a full queue costs the row,
//! counted and said once. Nothing here touches an order or the fold, and it runs on a transition, never per frame.
//! A process with no writer (a test driver, a tool) reports to nothing.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;

use vike_execution::EventSender;
use vike_model::accounts::account_keys::AccountNamed;
use vike_secrets::link_sessions::{LinkEvent, LinkKey, PLANE_TRADE};

/// How many transitions of the TRADE plane wait for the writer before one is lost. Its own queue, because an outage across a
/// session's symbols must cost no account's row: about 40 sockets, so far more than they say at once.
pub const LINK_TRADE_QUEUE: usize = 1024;
/// The same of the PRICE plane: one outage across a full session (2048 symbols) says 2048 words, twice over for two sessions.
pub const LINK_PRICE_QUEUE: usize = 4096;

/// How a session we ended reads.
pub const ENDED_BY_US: &str = "stopped by this process";

/// Where a transition goes, by its plane.
#[derive(Debug, Clone)]
pub struct VenueLinkSinks {
    /// The account sessions.
    pub trade: SyncSender<LinkEvent>,
    /// The price streams.
    pub price: SyncSender<LinkEvent>,
}

static SINK: OnceLock<VenueLinkSinks> = OnceLock::new();
static DROPPED: AtomicU64 = AtomicU64::new(0);

/// Give this process its writer. The first one stands: a daemon has one.
pub fn install(sinks: VenueLinkSinks) {
    let _ = SINK.set(sinks);
}

/// The rows a full queue has cost this process.
pub fn dropped() -> u64 {
    DROPPED.load(Ordering::Relaxed)
}

fn report(event: LinkEvent) {
    if let Some(sinks) = SINK.get() {
        route(sinks, event, &DROPPED);
    }
}

fn route(sinks: &VenueLinkSinks, event: LinkEvent, dropped: &AtomicU64) {
    let (LinkEvent::Opened { key, .. } | LinkEvent::Closed { key, .. }) = &event;
    let sink = if key.plane == PLANE_TRADE { &sinks.trade } else { &sinks.price };
    offer(sink, event, dropped);
}

/// Never waits: a full queue, or a writer gone, counts a lost row and says so once.
fn offer(sink: &SyncSender<LinkEvent>, event: LinkEvent, dropped: &AtomicU64) {
    if sink.try_send(event).is_err() && dropped.fetch_add(1, Ordering::Relaxed) == 0 {
        tracing::warn!(
            "link history: the writer is behind, so a venue session's row was lost; later ones are counted and not said \
             (the count is logged when the daemon stops). No connection is affected"
        );
    }
}

/// A session of `key` opened now.
pub fn opened(key: LinkKey) {
    report(LinkEvent::Opened { key, at_ms: vike_model::now_ms() });
}

/// The open session of `key` ended now, in the venue's words.
pub fn closed(key: LinkKey, how: &str) {
    report(LinkEvent::Closed {
        key,
        at_ms: vike_model::now_ms(),
        how: how.to_string(),
        ours: false,
    });
}

/// The open session of `key` ended now because we ended it.
pub fn closed_by_us(key: LinkKey) {
    report(LinkEvent::Closed {
        key,
        at_ms: vike_model::now_ms(),
        how: ENDED_BY_US.to_string(),
        ours: true,
    });
}

/// The key of an account's trade session: `route_key` is what its `EventSender` carries (`bybit#DEMO`), and the venue is the
/// roster venue whose account it names, read back by the model's one route-key rule ([`AccountNamed::from_route_key`]: the
/// account that renders to exactly this key). A label this build does not read keeps its venue (an `Unknown` account).
/// `lane` tells two sockets of one account apart. `None` on a bare lane, which names no account, and on a key no roster
/// venue minted (no bridge reports one).
#[must_use]
pub fn trade_key(route_key: Option<&str>, lane: &str) -> Option<LinkKey> {
    let route = route_key.filter(|r| !r.is_empty())?;
    let account = vike_model::VENUES
        .iter()
        .map(|venue| AccountNamed::from_route_key(venue, route))
        .find(|account| account.route_key() == route)?;
    Some(LinkKey {
        venue: account.venue().to_string(),
        route: route.to_string(),
        plane: PLANE_TRADE,
        lane: lane.to_string(),
    })
}

/// One session, from the moment it opened: [`Self::ended`] says how it ended, and a drop without that is our own stop,
/// so a bridge's every `return` on a stop needs no line.
#[derive(Debug)]
pub struct VenueLinkRecord(Option<LinkKey>);

impl VenueLinkRecord {
    /// The trade session of the account `events` speaks for opened now.
    #[must_use]
    pub fn of(events: &EventSender, lane: &str) -> Self {
        Self::trade(events.route_key.map(|key| key.as_str()), lane)
    }

    /// An account's trade session opened now. On a bare lane (no route key) it reports nothing.
    #[must_use]
    pub fn trade(route_key: Option<&str>, lane: &str) -> Self {
        let key = trade_key(route_key, lane);
        if let Some(key) = &key {
            opened(key.clone());
        }
        Self(key)
    }

    /// It ended, in the venue's words.
    pub fn ended(mut self, how: &str) {
        if let Some(key) = self.0.take() {
            closed(key, how);
        }
    }
}

impl Drop for VenueLinkRecord {
    fn drop(&mut self) {
        if let Some(key) = self.0.take() {
            closed_by_us(key);
        }
    }
}

/// What this process has reported, for a test that reads its own route's rows out of the one process-wide sink.
#[cfg(test)]
pub(crate) fn reported_for(route: &str) -> Vec<LinkEvent> {
    use std::sync::Mutex;
    use std::sync::mpsc::{Receiver, sync_channel};
    static HEARD: OnceLock<Mutex<(Receiver<LinkEvent>, Vec<LinkEvent>)>> = OnceLock::new();
    let heard = HEARD.get_or_init(|| {
        // The price half goes nowhere: these tests report the trade plane alone.
        let (trade, rx) = sync_channel(LINK_TRADE_QUEUE);
        let (price, _) = sync_channel(0);
        install(VenueLinkSinks { trade, price });
        Mutex::new((rx, Vec::new()))
    });
    let mut heard = heard.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (rx, all) = &mut *heard;
    all.extend(rx.try_iter());
    all.iter()
        .filter(|e| match e {
            LinkEvent::Opened { key, .. } | LinkEvent::Closed { key, .. } => key.route == route,
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::sync_channel;

    use super::*;

    /// ⚠ A burst across a session's symbols fills its own queue and costs no account's row.
    #[test]
    fn a_price_burst_costs_no_trade_row() {
        let (trade, trade_rx) = sync_channel(4);
        let (price, price_rx) = sync_channel(4);
        let sinks = VenueLinkSinks { trade, price };
        let dropped = AtomicU64::new(0);
        for at_ms in 0..100 {
            let key = LinkKey {
                venue: "hyperliquid".to_string(),
                route: format!("SYM{at_ms}"),
                plane: vike_secrets::link_sessions::PLANE_PRICE,
                lane: "quotes".to_string(),
            };
            route(&sinks, LinkEvent::Opened { key, at_ms }, &dropped);
        }
        assert_eq!(
            dropped.load(Ordering::Relaxed),
            96,
            "the price queue overflowed, and counted it"
        );
        let key = trade_key(Some("aster"), "pump").expect("a key");
        route(&sinks, LinkEvent::Opened { key, at_ms: 1 }, &dropped);
        assert_eq!(dropped.load(Ordering::Relaxed), 96, "the trade row was taken");
        assert_eq!((trade_rx.try_iter().count(), price_rx.try_iter().count()), (1, 4));
    }

    #[test]
    fn a_full_queue_costs_the_row_and_never_waits() {
        let (tx, rx) = sync_channel(1);
        let dropped = AtomicU64::new(0);
        let event = |at_ms| LinkEvent::Opened {
            key: trade_key(Some("aster"), "pump").expect("a key"),
            at_ms,
        };
        offer(&tx, event(1), &dropped);
        offer(&tx, event(2), &dropped);
        offer(&tx, event(3), &dropped);
        assert_eq!(dropped.load(Ordering::Relaxed), 2);
        assert_eq!(rx.try_iter().count(), 1);
        drop(rx);
        offer(&tx, event(4), &dropped);
        assert_eq!(
            dropped.load(Ordering::Relaxed),
            3,
            "a writer gone is the same lost row, and no panic"
        );
    }

    #[test]
    fn the_venue_is_the_word_before_the_label_and_a_bare_lane_has_no_key() {
        let labelled = trade_key(Some("bybit#DEMO"), "pump").expect("a key");
        assert_eq!((labelled.venue.as_str(), labelled.route.as_str()), ("bybit", "bybit#DEMO"));
        assert_eq!(trade_key(Some("aster"), "pump").expect("a key").venue, "aster");
        assert!(trade_key(None, "pump").is_none());
        assert!(trade_key(Some(""), "pump").is_none());
    }

    /// The model's rule reads every roster venue's key as the hand split before it did (the word before the first `#`),
    /// a label this build does not read included; a key no roster venue minted names no account and writes no row.
    #[test]
    fn the_model_rule_splits_every_roster_key_as_the_hand_split_did() {
        for venue in vike_model::VENUES {
            for suffix in ["", "#DEMO", "#A1", "#DEFAULT", "#SESSION_SHAPE", "#demo", "#", "#X#Y"] {
                let route = format!("{venue}{suffix}");
                let key = trade_key(Some(&route), "pump").expect("a roster key");
                let hand = route.split_once('#').map_or(route.as_str(), |(venue, _)| venue);
                assert_eq!((key.venue.as_str(), key.route.as_str()), (hand, route.as_str()));
            }
        }
        assert!(trade_key(Some("sim#A"), "pump").is_none());
    }

    /// The shape a bridge's `return` on a stop relies on.
    #[test]
    fn a_session_says_how_it_ended_and_a_drop_is_our_own_stop() {
        let route = "okx#SESSION_SHAPE";
        reported_for(route);
        VenueLinkRecord::trade(Some(route), "a").ended("server closed: 1006");
        drop(VenueLinkRecord::trade(Some(route), "b"));
        let heard = reported_for(route);
        let hows: Vec<(String, bool)> = heard
            .iter()
            .filter_map(|e| match e {
                LinkEvent::Closed { how, ours, .. } => Some((how.clone(), *ours)),
                LinkEvent::Opened { .. } => None,
            })
            .collect();
        assert_eq!(heard.len(), 4, "{heard:?}");
        assert_eq!(
            hows,
            vec![("server closed: 1006".to_string(), false), (ENDED_BY_US.to_string(), true)]
        );
    }
}
