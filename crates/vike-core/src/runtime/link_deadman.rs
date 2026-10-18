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
//! * [`FeedStatus::Disconnected`] on an armed venue OPENS a grace window for that `(venue, symbol)`
//!   AND THE SOURCE THAT DISCLOSED IT.
//! * [`FeedStatus::Live`] from the same source for the same key CLOSES that source's window (and
//!   clears its trip, re-arming that link). A `Live` from a DIFFERENT source closes nothing.
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
//!   and `crates/bridges/polymarket/src/market_feed/update.rs`'s `on_frame` both recover that way). The
//!   production sequence on a fresh mount is therefore `Gap` first and `Live` only if it comes
//!   back — so a prior-`Live` gate made the switch blind to exactly the outage it exists for: the
//!   FIRST socket death of a mount, the one that never recovers. The startup dial-up it was
//!   supposed to protect is absorbed by the GRACE instead, which is the right instrument for it:
//!   `vike_config::MIN_LINK_DEADMAN_GRACE_MS` is a whole ordinary reconnect cycle and the default
//!   is five of them, while a link that cannot be established at all for that long, with orders
//!   resting at the venue, is the hazard rather than an exception to it.
//!
//! ## One window per SOURCE
//!
//! With the tradehub reaching the world through two hubs, three links can each disclose
//! `Disconnected`/`Live` for one `(venue, symbol)`: the venue's own socket, the account's trade link
//! to the venue gateway, and the strategy-lane feed's link to the datahub
//! ([`vike_venues::link_deadman::LinkSource`], derived from the status's `stream` word:
//! `TRADE_LINK_STREAM` and `STRAT_LINK_STREAM` are reserved, every other word is the venue's). The
//! latch therefore keeps a window per `(venue, symbol, source)`: a strat-link `Live` must not erase an
//! outage the venue's book socket disclosed, and a venue `Live` must not erase the trade link's. Any
//! source's window past the grace trips, and **each window acts ONCE for its OWN outage**: it is
//! reported by [`LinkDeadMan::check`] when it expires and not again until ITS OWN `Live` closes it.
//! ⚠ **There is NO cross-source suppression, and that is a correction.** The first version let a
//! window that expired while another source of the key still held a trip go unreported ("the acting
//! half already cancelled and halted"). That premise is false: an operator clears HALT with a hub
//! link still down, a `cancel_all` trip engages no HALT at all so orders placed after it rest
//! unguarded, and a source that never reports `Live` (a producer bug, a process that exited
//! mid-gap) held its trip for the life of the process and silenced every other source of the key.
//! The acting half is idempotent where it matters (HALT is a state and a sentinel FILE's existence,
//! and a repeated venue `MassCancel` re-requests the cancel of whatever is live NOW, which is the
//! point), and `sweep_link_deadman` already folds the windows of one sweep into one cancel pass per
//! venue. A tripped source's own `Live` reports a recovery. `Stale` still never counts, and an
//! unarmed venue still costs one set lookup and no allocation.
//!
//! ## Which venues arm is NOT this module's question
//!
//! `vike_venues::link_deadman_default` is the per-venue table (one NAMED row per roster venue, read
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
use vike_venues::link_deadman::LinkSource;

use super::deadman::DeadManAction;

/// Opt-in CONNECTION-state dead-man configuration. Hung off [`crate::CoreConfig`] as an `Option`,
/// `None` (default) = disabled and byte-identical to a runtime that never heard of the feature.
///
/// The ACTION is deliberately [`DeadManAction`], shared with the silence switch: "what does a trip
/// do" is the same question for both, one `policy` key answers it (`deadman_action`), and a
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
    /// entirely. Folded by the composition root from `vike_venues::link_deadman_default` and the
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
    /// A link that had TRIPPED is back AND no other source of its `(venue, symbol)` still holds a
    /// trip. The switch re-arms for the next outage; `Halted` and the HALT sentinel do NOT clear —
    /// that is an operator's decision, exactly as with the silence switch.
    Recovered,
    /// A link that had TRIPPED is back, but ANOTHER source of the same `(venue, symbol)` still
    /// holds its own trip. This source re-armed (each window is its own latch); the key as a whole
    /// is NOT clear, so the runtime must not say it is.
    RecoveredWhileAnotherSourceStaysTripped,
}

/// One armed link's state. Deliberately two fields and no clock: every transition is unit-tested
/// with injected timestamps and no waiting.
///
/// ⚠ An entry exists ONLY for a `(venue, symbol, source)` that has actually been disclosed
/// `Disconnected` at least once — a `Live` on a link with no entry STORES nothing (the lookup key
/// is built from two short `String`s, off the hot fold, and dropped). A mount whose armed venues
/// never drop a socket therefore carries an EMPTY map for the life of the process.
#[derive(Debug, Default, Clone, Copy)]
struct LinkState {
    /// When the current outage was first disclosed, `None` while the link is up. Set on the FIRST
    /// `Disconnected` of an outage and not moved by later ones, so a bridge that re-discloses a
    /// gap cannot keep pushing the deadline out.
    down_since: Option<i64>,
    /// `true` once THIS outage tripped — so this window acts exactly once per outage rather than at
    /// every sweep while the link stays down. Cleared ONLY by a `Live` FROM THE SAME SOURCE, and it
    /// vetoes nothing about any OTHER source of the key (see the module doc's "One window per
    /// SOURCE").
    tripped: bool,
}

/// One window that expired on a [`LinkDeadMan::check`]: what the acting half acts on, and what an
/// operator reads at 3am to learn WHICH link tripped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinkTrip {
    pub(crate) venue: String,
    pub(crate) symbol: String,
    /// The source whose window expired.
    pub(crate) source: LinkSource,
    /// The OTHER sources of this `(venue, symbol)` that already held a trip when this one tripped
    /// (including one marked earlier in the same sweep): a non-empty list means this is a SECOND
    /// source tripping and the acting half is acting again, on purpose.
    pub(crate) also_tripped: Vec<LinkSource>,
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
    /// Per-`(venue, symbol, source)` state: ONE window per disclosing source, so a `Live` from one
    /// source closes only that source's window. A `BTreeMap` rather than a hash map for the same
    /// reason the rest of this crate prefers ordered containers: the sweep iterates it, and a
    /// deterministic order makes a multi-link trip's cancel ORDER reproducible in a test (and
    /// keeps every source of one `(venue, symbol)` adjacent).
    links: BTreeMap<(String, String, LinkSource), LinkState>,
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

    /// Record a feed-status transition for `(venue, symbol)` disclosed by `source` at `now_ms`.
    /// `source` is what [`LinkSource::of`] made of the status's `stream` word (the caller derives
    /// it): a venue lane's word is [`LinkSource::Venue`], the two reserved words are the trade link
    /// and the strategy-lane link. It is a TYPE, not a third `&str`, so `symbol` and the stream
    /// word cannot be swapped by accident (the review's F6).
    ///
    /// ⚠ [`FeedStatus::Stale`] returns without touching anything, and that line is the entire
    /// difference between this switch and the one it replaces. Do not "handle" it.
    pub(crate) fn observe(
        &mut self,
        venue: &str,
        symbol: &str,
        source: LinkSource,
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
                // No entry ⇒ this source has never been disclosed down on this key, so there is
                // nothing to close and nothing to allocate. `Live` is emitted by these bridges only
                // to CLOSE a gap (see the module doc), so the no-entry case is the ordinary one.
                let Some(st) = self.links.get_mut(&(venue.to_string(), symbol.to_string(), source))
                else {
                    return LinkObservation::Quiet;
                };
                st.down_since = None;
                if !std::mem::take(&mut st.tripped) {
                    return LinkObservation::Quiet;
                }
                if self.other_sources_tripped(venue, symbol, source).is_empty() {
                    LinkObservation::Recovered
                } else {
                    LinkObservation::RecoveredWhileAnotherSourceStaysTripped
                }
            }
            FeedStatus::Disconnected => {
                // ⚠ No prior-`Live` condition, and the module doc carries why: a first connect
                // discloses nothing on this roster, so a gate on it would make the switch blind to
                // the FIRST link death of a mount — the outage it exists for.
                let st =
                    self.links.entry((venue.to_string(), symbol.to_string(), source)).or_default();
                if st.down_since.is_none() {
                    st.down_since = Some(now_ms);
                }
                LinkObservation::Quiet
            }
        }
    }

    /// Forget the TRADE-LINK window of `(venue, symbol)`: the account that owned that link was stopped on
    /// purpose (decision 0169, slice S2), so no `Live` will ever come to close an outage disclosed before the
    /// stop, and the grace would expire into a `CancelAllAndHalt` for the venue. Only the trade link's
    /// window goes: the venue's own socket and the strategy-lane link are shared by every account.
    /// The caller guarantees no other running account uses the same `(venue, symbol)` link.
    pub(crate) fn forget_trade_link(&mut self, venue: &str, symbol: &str) {
        self.links.remove(&(venue.to_string(), symbol.to_string(), LinkSource::TradeLink));
    }

    /// Evaluate every armed link at `now_ms`. Returns one [`LinkTrip`] per WINDOW whose grace has
    /// EXPIRED on this sweep, in key order (`Venue < TradeLink < StratLink` within one
    /// `(venue, symbol)`). **Each window acts once for its own outage and no other window's state
    /// can suppress it**: a window already tripped is silent until a `Live` from ITS source re-arms
    /// it, and a trip held by one source says nothing about another (the module doc's "One window
    /// per SOURCE" carries why the earlier cross-source dedupe was removed). Two sources of one
    /// key expiring in the SAME sweep are two trips here; the sweep folds them into one cancel pass
    /// per venue.
    ///
    /// Returns trips rather than acting, for the reason [`super::deadman::DeadMan::check`] does the
    /// same: the acting half needs `&mut self` on the whole `CoreThread`, and keeping the decision
    /// pure is what makes it testable with no engine, no client and no clock.
    ///
    /// `idle` names the armed venues that hold nothing to protect right now (a price-only venue with no
    /// live order and no open position, [`crate::CoreConfig::price_only`]): their windows are
    /// DISCARDED, tripped or not, so such a venue neither trips nor carries an old outage into the day
    /// something is ordered. A window that opens while the venue holds something and is still down at
    /// the sweep after the grace trips like any other. Empty for every other core. (The window is not
    /// even OPENED for an idle venue: `CoreThread::link_window_may_open` gates the observe, so this
    /// discard only has to catch a venue that BECAME idle. An outage already in progress when a venue
    /// first holds something is not seen: a window needs a disclosed `Disconnected` to open.)
    pub(crate) fn check(
        &mut self,
        now_ms: i64,
        idle: &std::collections::BTreeSet<String>,
    ) -> Vec<LinkTrip> {
        if !idle.is_empty() {
            self.links.retain(|(venue, _, _), _| !idle.contains(venue));
        }
        let grace_ms = self.grace_ms;
        let expired: Vec<(String, String, LinkSource)> = self
            .links
            .iter()
            .filter(|(_, st)| {
                !st.tripped && st.down_since.is_some_and(|since| now_ms - since > grace_ms)
            })
            .map(|(key, _)| key.clone())
            .collect();
        let mut tripped: Vec<LinkTrip> = Vec::new();
        for key in expired {
            let (venue, symbol, source) = key.clone();
            // Read BEFORE marking this window, so the list is the OTHER sources only (and a source
            // marked earlier in this very loop is in it).
            let also_tripped = self.other_sources_tripped(&venue, &symbol, source);
            if let Some(st) = self.links.get_mut(&key) {
                st.tripped = true;
            }
            tripped.push(LinkTrip { venue, symbol, source, also_tripped });
        }
        tripped
    }

    /// The sources of `(venue, symbol)` OTHER than `source` that currently hold a trip, in key
    /// order. Diagnostics only: nothing decides on it.
    fn other_sources_tripped(
        &self,
        venue: &str,
        symbol: &str,
        source: LinkSource,
    ) -> Vec<LinkSource> {
        self.links
            .iter()
            .filter(|((v, s, src), st)| v == venue && s == symbol && *src != source && st.tripped)
            .map(|((_, _, src), _)| *src)
            .collect()
    }

    /// Whether ANY source of `(venue, symbol)` currently holds a trip.
    #[cfg(test)]
    pub(crate) fn is_tripped(&self, venue: &str, symbol: &str) -> bool {
        self.links.iter().any(|((v, s, _), st)| v == venue && s == symbol && st.tripped)
    }

    /// Whether the window of ONE source of `(venue, symbol)` is open (disclosed down, not yet closed).
    #[cfg(test)]
    pub(crate) fn is_down(&self, venue: &str, symbol: &str, source: LinkSource) -> bool {
        self.links
            .get(&(venue.to_string(), symbol.to_string(), source))
            .is_some_and(|st| st.down_since.is_some())
    }
}

#[path = "tests/link_deadman_unit.rs"]
#[cfg(test)]
mod link_deadman_unit_tests;
