//! **The CONNECTION-state dead-man** (M13) — the switch that trips on a socket the BRIDGE reports
//! dead, and stays quiet through a market that merely closed. No Python twin; a Rust-native
//! operational safeguard, and the successor to [`super::deadman`]'s silence-observing latch rather
//! than a variant of it.
//!
//! ## The re-ruling this module IS
//!
//! [`super::deadman`] observes SILENCE: it counts ingest and trips when nothing has arrived for a
//! while. That switch shipped one morning as a default-ON safety net and was made opt-in the same
//! day, because silence is what a CLOSED market looks like — an FX or equities mount halted itself
//! at every session close and opened the next session `Halted`. `vike_config::Policy::
//! deadman_timeout_ms`'s doc carries the three halts an armed default bought;
//! `docs/decisions/0038-the-dead-man-observes-the-connection-not-silence.md` is the record. This module is the correct mechanism for that
//! safety argument: it watches the per-`(venue, symbol)` [`vike_model::FeedStatus`] the bridges
//! DISCLOSE, so a venue that stopped ticking because its market closed is invisible to it.
//!
//! ## The signal, exactly
//!
//! * [`FeedStatus::Disconnected`] on an armed venue OPENS a grace window for that `(venue, symbol)`.
//! * [`FeedStatus::Live`] for the same key CLOSES it (and clears a trip, re-arming the link).
//! * [`FeedStatus::Stale`] NEVER counts, in either direction. `Stale` is "no fresh price exists",
//!   which is an accurate disclosure of a weekend on an FX venue
//!   (`crates/bridges/oanda/src/market_feed.rs`'s module doc says so of that venue's own
//!   `FRESHNESS_THRESHOLD_QUOTES`) — reacting to it would be the silence switch again.
//! * A `Disconnected` opens the window whether or not that key was ever seen `Live`, and ⚠ **that
//!   is a correction, not a simplification.** The first draft gated the window on a prior `Live`,
//!   mirroring [`super::deadman`]'s never-observed rule — and the rule is unreachable here, because
//!   no bridge on this roster discloses a `Live` on a FIRST successful connect:
//!   `vike_bridge_core::stream_health`'s `StreamHealth::recover` returns `None` when no gap is
//!   open, so `Live` exists ONLY to close a gap (`vike_bridge_core::depth`'s `run_depth_session`
//!   and `crates/bridges/polymarket/src/market_feed.rs`'s `on_frame` both recover that way). The
//!   production sequence on a fresh mount is therefore `Gap` first and `Live` only if it comes
//!   back — so a prior-`Live` gate made the switch blind to exactly the outage it exists for: the
//!   FIRST socket death of a mount, the one that never recovers. The startup dial-up it was
//!   supposed to protect is absorbed by the GRACE instead, which is the right instrument for it:
//!   `vike_config::MIN_LINK_DEADMAN_GRACE_MS` is a whole ordinary reconnect cycle and the default
//!   is five of them, while a link that cannot be established at all for that long, with orders
//!   resting at the venue, is the hazard rather than an exception to it.
//!
//! ## Which venues arm is NOT this module's question
//!
//! `vike_model::link_deadman_default` is the per-venue table (one NAMED row per roster venue, read
//! from the emitter it cites), and the composition root folds it into [`LinkDeadManConfig::venues`]
//! before the core is built. A `(venue, symbol)` whose venue is not in that set is dropped at
//! [`LinkDeadMan::observe`] without allocating — so an FX venue mounted beside an armed crypto one
//! costs one set lookup per status change and nothing else.
//!
//! ## Off the hot fold
//! The latch is touched from exactly two places: [`LinkDeadMan::observe`], on the
//! `Ingest::StreamStatus` arm — an OCCASIONAL control event the producer fires only on a transition,
//! never per tick — and [`LinkDeadMan::check`], at the drain-loop boundary on the
//! [`super::TimerKind::LinkDeadManSweep`] cadence. The tick/quote/trade/bar/book fold has no branch
//! for this feature at all, and `None` on [`crate::CoreConfig::link_deadman`] (the default) builds
//! no latch, arms no timer and contributes nothing to the waker cadence.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use vike_model::FeedStatus;

use super::deadman::DeadManAction;

/// Opt-in CONNECTION-state dead-man configuration. Hung off [`crate::CoreConfig`] as an `Option`,
/// `None` (default) = disabled and byte-identical to a runtime that never heard of the feature.
///
/// The ACTION is deliberately [`DeadManAction`], shared with the silence switch: "what does a trip
/// do" is the same question for both, one `policy.toml` key answers it (`deadman_action`), and a
/// second enum would be a second answer to one question.
#[derive(Debug, Clone)]
pub struct LinkDeadManConfig {
    /// How long a link may stay DISCONNECTED before the switch trips. Compared strictly
    /// (`now - down_since > grace` ⇒ trip), mirroring [`super::deadman`]'s freshness boundary.
    pub grace: Duration,
    /// What a trip does. [`DeadManAction::CancelAllAndHalt`] additionally engages HALT — see
    /// [`crate::runtime::CoreThread::sweep_link_deadman`] for why that half is process-wide while
    /// the CANCEL is scoped to the venue whose link died.
    pub action: DeadManAction,
    /// The venues this switch is ARMED for. A status change on any other venue is ignored
    /// entirely. Folded by the composition root from `vike_model::link_deadman_default` and the
    /// venues actually mounted; an EMPTY set is a legal, inert configuration (every mounted venue
    /// was session-bounded or discloses no disconnect), and the mount says so per venue.
    pub venues: BTreeSet<String>,
    /// The HALT sentinel to write when the action engages HALT — the SAME file
    /// [`super::DeadManConfig::halt_file`] names and a manual `touch HALT` writes, supplied by the
    /// BINARY for the same reason (env reads stay in binaries; `vike-core` carries no
    /// `vike-bridge-core` edge). `None` skips the file write; the in-process `Halted` gate still
    /// engages.
    pub halt_file: Option<PathBuf>,
}

/// What one [`LinkDeadMan::observe`] call is worth reporting. The runtime logs the recovery and
/// ignores the rest — one `tracing::info!` per link that came back from a TRIPPED outage, never one
/// per status change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinkObservation {
    /// Nothing an operator needs told: an unarmed venue, a `Stale`, a `Live` on a link that was
    /// never down, a `Disconnected` inside an already-open window, or a `Live` that closed a
    /// window which had not yet tripped.
    Quiet,
    /// A link that had TRIPPED is back. The switch re-arms for the next outage; `Halted` and the
    /// HALT sentinel do NOT clear — that is an operator's decision, exactly as with the silence
    /// switch.
    Recovered,
}

/// One armed link's state. Deliberately two fields and no clock: every transition is unit-tested
/// with injected timestamps and zero sleeps.
///
/// ⚠ An entry exists ONLY for a `(venue, symbol)` that has actually been disclosed `Disconnected`
/// at least once — a `Live` on a link with no entry allocates nothing. A mount whose armed venues
/// never drop a socket therefore carries an EMPTY map for the life of the process.
#[derive(Debug, Default, Clone, Copy)]
struct LinkState {
    /// When the current outage was first disclosed, `None` while the link is up. Set on the FIRST
    /// `Disconnected` of an outage and not moved by later ones, so a bridge that re-discloses a
    /// gap cannot keep pushing the deadline out.
    down_since: Option<i64>,
    /// `true` once THIS outage tripped — so the trip fires exactly once per outage rather than at
    /// every sweep while the link stays down. Cleared by a `Live`.
    tripped: bool,
}

/// The pure trip-logic state machine. Clock-free: the caller feeds `now_ms` at both entry points,
/// so every transition is unit-tested with an injected clock. Built once from a
/// [`LinkDeadManConfig`] and owned by the runtime's `CoreThread` (only when the feature is enabled).
#[derive(Debug)]
pub(crate) struct LinkDeadMan {
    /// The grace in ms, clamped `>= 1` so `now - down_since > grace` is a well-defined strict
    /// comparison even for a pathological zero (which the file edge already refuses).
    grace_ms: i64,
    /// What a trip does.
    action: DeadManAction,
    /// The armed venue set — see [`LinkDeadManConfig::venues`].
    venues: BTreeSet<String>,
    /// Per-`(venue, symbol)` state. A `BTreeMap` rather than a hash map for the same reason the
    /// rest of this crate prefers ordered containers: the sweep iterates it, and a deterministic
    /// order makes a multi-link trip's cancel ORDER reproducible in a test.
    links: BTreeMap<(String, String), LinkState>,
}

impl LinkDeadMan {
    /// Build from a [`LinkDeadManConfig`]. The grace is clamped to `>= 1ms`.
    pub(crate) fn new(cfg: &LinkDeadManConfig) -> Self {
        LinkDeadMan {
            grace_ms: (cfg.grace.as_millis() as i64).max(1),
            action: cfg.action,
            venues: cfg.venues.clone(),
            links: BTreeMap::new(),
        }
    }

    /// The grace in ms — the runtime derives the sweep cadence (`grace / 2`) from it, mirroring
    /// the stuck-order watchdog and the silence switch.
    pub(crate) fn grace_ms(&self) -> i64 {
        self.grace_ms
    }

    /// What a trip does — read by the sweep, which owns the acting half.
    pub(crate) fn action(&self) -> DeadManAction {
        self.action
    }

    /// Record a feed-status transition for `(venue, symbol)` at `now_ms`.
    ///
    /// ⚠ [`FeedStatus::Stale`] returns without touching anything, and that line is the entire
    /// difference between this switch and the one it replaces. Do not "handle" it.
    pub(crate) fn observe(
        &mut self,
        venue: &str,
        symbol: &str,
        status: FeedStatus,
        now_ms: i64,
    ) -> LinkObservation {
        if !self.venues.contains(venue) {
            return LinkObservation::Quiet; // an unarmed venue costs one set lookup and no alloc
        }
        match status {
            // Silence is not a disconnect. A closed market's honest "no fresh price exists" must
            // move nothing here — neither opening a window nor closing one.
            FeedStatus::Stale => LinkObservation::Quiet,
            FeedStatus::Live => {
                // No entry ⇒ this link has never been disclosed down, so there is nothing to
                // close and nothing to allocate. `Live` is emitted by these bridges only to CLOSE
                // a gap (see the module doc), so the no-entry case is the ordinary one.
                let Some(st) = self.links.get_mut(&(venue.to_string(), symbol.to_string())) else {
                    return LinkObservation::Quiet;
                };
                st.down_since = None;
                if std::mem::take(&mut st.tripped) {
                    LinkObservation::Recovered
                } else {
                    LinkObservation::Quiet
                }
            }
            FeedStatus::Disconnected => {
                // ⚠ No prior-`Live` condition, and the module doc carries why: a first connect
                // discloses nothing on this roster, so a gate on it would make the switch blind to
                // the FIRST link death of a mount — the outage it exists for.
                let st = self.links.entry((venue.to_string(), symbol.to_string())).or_default();
                if st.down_since.is_none() {
                    st.down_since = Some(now_ms);
                }
                LinkObservation::Quiet
            }
        }
    }

    /// Evaluate every armed link at `now_ms`. Returns the `(venue, symbol)` keys whose grace has
    /// EXPIRED on this sweep — each exactly once per outage; a link already tripped is silent until
    /// a `Live` re-arms it.
    ///
    /// Returns keys rather than acting, for the reason [`super::deadman::DeadMan::check`] does the
    /// same: the acting half needs `&mut self` on the whole `CoreThread`, and keeping the decision
    /// pure is what makes it testable with no engine, no client and no clock.
    pub(crate) fn check(&mut self, now_ms: i64) -> Vec<(String, String)> {
        let mut tripped = Vec::new();
        for (key, st) in self.links.iter_mut() {
            let Some(down_since) = st.down_since else { continue };
            if st.tripped || now_ms - down_since <= self.grace_ms {
                continue;
            }
            st.tripped = true;
            tripped.push(key.clone());
        }
        tripped
    }

    #[cfg(test)]
    pub(crate) fn is_tripped(&self, venue: &str, symbol: &str) -> bool {
        self.links.get(&(venue.to_string(), symbol.to_string())).is_some_and(|st| st.tripped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(grace_ms: u64, venues: &[&str]) -> LinkDeadManConfig {
        LinkDeadManConfig {
            grace: Duration::from_millis(grace_ms),
            action: DeadManAction::CancelAllAndHalt,
            venues: venues.iter().map(|v| (*v).to_string()).collect(),
            halt_file: None,
        }
    }

    fn latch(grace_ms: u64, venues: &[&str]) -> LinkDeadMan {
        LinkDeadMan::new(&cfg(grace_ms, venues))
    }

    /// (a) The core case: a disclosed disconnect, a grace that expires, ONE trip.
    #[test]
    fn a_disconnect_outlasting_the_grace_trips_once() {
        let mut dm = latch(1000, &["binance"]);
        dm.observe("binance", "BTCUSDT", FeedStatus::Live, 0);
        dm.observe("binance", "BTCUSDT", FeedStatus::Disconnected, 100);

        assert!(dm.check(1100).is_empty(), "lag == grace is still inside it (strict >)");
        assert_eq!(
            dm.check(1101),
            vec![("binance".to_string(), "BTCUSDT".to_string())],
            "one ms past the grace ⇒ trip"
        );
        assert!(dm.check(9999).is_empty(), "no re-fire within the same outage");
        assert!(dm.is_tripped("binance", "BTCUSDT"));
    }

    /// (b) The property the grace exists for: an ORDINARY reconnect is silent. A `Live` inside the
    /// window closes it, and no later sweep can trip on that outage.
    #[test]
    fn a_reconnect_inside_the_grace_is_silent() {
        let mut dm = latch(1000, &["bybit"]);
        dm.observe("bybit", "BTCUSDT", FeedStatus::Live, 0);
        dm.observe("bybit", "BTCUSDT", FeedStatus::Disconnected, 100);
        assert!(dm.check(500).is_empty());
        // the venue's backoff + re-dial lands well inside the window
        assert_eq!(dm.observe("bybit", "BTCUSDT", FeedStatus::Live, 700), LinkObservation::Quiet);
        assert!(dm.check(10_000).is_empty(), "a closed window cannot trip later");
    }

    /// (c) ⚠ **The rule the whole switch turns on.** `Stale` moves nothing — it neither opens a
    /// window nor closes one — because a weekend on an FX venue is disclosed as exactly this.
    #[test]
    fn stale_never_trips_and_never_clears() {
        let mut dm = latch(1000, &["oanda"]);
        dm.observe("oanda", "EUR_USD", FeedStatus::Live, 0);
        for t in [100, 5_000, 100_000, 172_800_000] {
            assert_eq!(
                dm.observe("oanda", "EUR_USD", FeedStatus::Stale, t),
                LinkObservation::Quiet
            );
            assert!(dm.check(t).is_empty(), "a 48h weekend of Stale must not trip at t={t}");
        }
        // ...and it did not silently CLOSE an outage either: a real disconnect after all that
        // Stale still opens a window and still trips.
        dm.observe("oanda", "EUR_USD", FeedStatus::Disconnected, 172_800_000);
        assert!(dm.check(172_800_500).is_empty());
        assert_eq!(dm.check(172_801_001).len(), 1, "a REAL disconnect still trips after Stale");
    }

    /// (d) ⚠ **The FIRST link death of a mount trips, with no `Live` ever observed** — the
    /// headline outage, and the one a prior-`Live` gate silently excluded. No bridge on this
    /// roster discloses `Live` on a first successful connect (`StreamHealth::recover` is a no-op
    /// with no gap open), so under that gate a socket that came up, died and never returned
    /// produced no window at all. The module doc carries the evidence.
    #[test]
    fn a_first_link_death_trips_without_any_prior_live() {
        let mut dm = latch(1000, &["okx"]);
        dm.observe("okx", "BTC-USDT-SWAP", FeedStatus::Disconnected, 0);
        assert!(dm.check(500).is_empty(), "still inside the grace");
        assert_eq!(
            dm.check(1001),
            vec![("okx".to_string(), "BTC-USDT-SWAP".to_string())],
            "a link disclosed down and never disclosed up is exactly what this switch is for"
        );
    }

    /// (d2) ...and the startup dial-up the retired gate was reaching for is absorbed by the GRACE
    /// instead: a venue that fails its first connects and then comes up inside the window cancels
    /// nothing. This is the property that makes (d) safe rather than trigger-happy.
    #[test]
    fn a_startup_dial_that_succeeds_inside_the_grace_is_silent() {
        let mut dm = latch(30_000, &["binance"]);
        // a failed first dial is disclosed as a gap by the depth driver's `enter_gap`
        dm.observe("binance", "BTCUSDT", FeedStatus::Disconnected, 0);
        for t in [3_000, 6_000, 9_000] {
            dm.observe("binance", "BTCUSDT", FeedStatus::Disconnected, t); // backoff re-dials
            assert!(dm.check(t).is_empty());
        }
        dm.observe("binance", "BTCUSDT", FeedStatus::Live, 23_000); // one full ordinary cycle
        assert!(dm.check(1_000_000).is_empty(), "the link came up inside the grace");
    }

    /// (d3) A `Live` on a link that was never down allocates nothing — the map holds an entry per
    /// link that has actually been disclosed DOWN, not one per subscription.
    #[test]
    fn a_live_on_a_healthy_link_costs_no_state() {
        let mut dm = latch(1000, &["binance"]);
        for s in ["BTCUSDT", "ETHUSDT", "SOLUSDT"] {
            assert_eq!(dm.observe("binance", s, FeedStatus::Live, 0), LinkObservation::Quiet);
        }
        assert!(dm.links.is_empty(), "a link that never dropped is not tracked");
    }

    /// (e) Recovery re-arms, and says so exactly once: the `Live` that clears a TRIPPED outage
    /// reports `Recovered`, a second `Live` reports nothing, and a NEW outage trips again.
    #[test]
    fn recovery_reports_once_and_re_arms() {
        let mut dm = latch(1000, &["binance"]);
        dm.observe("binance", "BTCUSDT", FeedStatus::Live, 0);
        dm.observe("binance", "BTCUSDT", FeedStatus::Disconnected, 0);
        assert_eq!(dm.check(2000).len(), 1, "first outage trips");

        assert_eq!(
            dm.observe("binance", "BTCUSDT", FeedStatus::Live, 3000),
            LinkObservation::Recovered
        );
        assert_eq!(
            dm.observe("binance", "BTCUSDT", FeedStatus::Live, 3100),
            LinkObservation::Quiet,
            "a second Live is not a second recovery"
        );
        assert!(!dm.is_tripped("binance", "BTCUSDT"));

        dm.observe("binance", "BTCUSDT", FeedStatus::Disconnected, 4000);
        assert_eq!(dm.check(5001).len(), 1, "a second outage trips again after the re-arm");
    }

    /// (f) A venue outside the armed set is invisible — no state, no trip. This is what makes an FX
    /// venue mounted beside an armed crypto one free rather than merely harmless.
    #[test]
    fn an_unarmed_venue_is_never_tracked() {
        let mut dm = latch(1000, &["binance"]);
        dm.observe("ig", "IX.D.FTSE.DAILY.IP", FeedStatus::Live, 0);
        dm.observe("ig", "IX.D.FTSE.DAILY.IP", FeedStatus::Disconnected, 0);
        assert!(dm.check(10_000_000).is_empty(), "an unarmed venue can never trip");
        assert!(dm.links.is_empty(), "...and costs no state at all");
    }

    /// (g) A re-disclosed gap does NOT push the deadline out. A bridge that emits a second
    /// `Disconnected` mid-outage (a per-token feed reconnecting one shard at a time) must not be
    /// able to hold the switch open forever.
    #[test]
    fn a_second_disconnect_does_not_extend_the_window() {
        let mut dm = latch(1000, &["polymarket"]);
        dm.observe("polymarket", "TOK", FeedStatus::Live, 0);
        dm.observe("polymarket", "TOK", FeedStatus::Disconnected, 100);
        for t in [200, 400, 900, 1_050] {
            dm.observe("polymarket", "TOK", FeedStatus::Disconnected, t);
        }
        assert_eq!(dm.check(1101).len(), 1, "the window still expires 1000 ms after the FIRST gap");
    }

    /// (h) Two links on one armed venue trip independently, and one venue's outage leaves another
    /// venue's link untouched — the per-key scoping the runtime's venue-scoped cancel rests on.
    #[test]
    fn links_are_tracked_per_venue_and_symbol() {
        let mut dm = latch(1000, &["binance", "bybit"]);
        for (v, s) in [("binance", "BTCUSDT"), ("binance", "ETHUSDT"), ("bybit", "BTCUSDT")] {
            dm.observe(v, s, FeedStatus::Live, 0);
        }
        dm.observe("binance", "BTCUSDT", FeedStatus::Disconnected, 0);
        assert_eq!(
            dm.check(2000),
            vec![("binance".to_string(), "BTCUSDT".to_string())],
            "only the link that died trips"
        );
        assert!(!dm.is_tripped("binance", "ETHUSDT"));
        assert!(!dm.is_tripped("bybit", "BTCUSDT"));
    }
}
