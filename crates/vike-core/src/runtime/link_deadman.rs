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
//! The acting half is idempotent where it matters (the venue halt is a state whose first prior is
//! kept, and a repeated venue `MassCancel` re-requests the cancel of whatever is live NOW, which is the
//! point), and `sweep_link_deadman` already folds the windows of one sweep into one cancel pass per
//! venue. A tripped source's own `Live` reports a recovery. `Stale` still never counts, and an
//! unarmed venue still costs one set lookup and no allocation.
//!
//! ## The halt is the VENUE's (owner ruling 2026-10-18, decision 0180)
//!
//! A trip under [`DeadManAction::CancelAllAndHalt`] halts the engines of the venue whose link died
//! and NOTHING else: no other venue's engine changes state and no sentinel file is written (the
//! sentinel is read by every venue's `ExecActor`, so writing it would halt them all). This latch
//! keeps the overlay that makes that reversible: per venue, per engine index, the state the engine
//! held BEFORE the halt ([`LinkDeadMan::record_halt`]). The venue is released
//! ([`LinkDeadMan::take_recovered`]) once NONE of its windows is open — every source of every symbol
//! has reported `Live`, been forgotten, or been discarded as idle — and each engine goes back to its
//! PRIOR state, never to `Active` (a venue halted while the drawdown latch held `Reducing` returns
//! to `Reducing`). The operator's node-wide `SetTradingState` ([`LinkDeadMan::resume_halts`]) is the
//! operator's state and nothing re-arms over it, but it does NOT reopen a venue with an OPEN window:
//! that venue stays halted (its prior becomes the operator's state) until its links are up or
//! `Command::ResumeVenue` is forced. A link merely remembered down while an idle price-only venue
//! holds nothing ([`LinkDeadMan::remember_down`]) is not a hold: that is the "discarded as idle" release
//! above, deliberate (decision 0180), and the node-wide resume does not claim to keep such a venue. A venue tripped with NO engine here is recorded halted too
//! ([`LinkDeadMan::record_venue_halt`]), so an account hot-started later joins the halt. A node-wide
//! stop (the silence dead-man) wipes the overlay ([`LinkDeadMan::clear_halts`]).
//! The overlay lives here, not on the core, so a core without the switch carries nothing.
//!
//! The operator can also release ONE venue's overlay early, `Command::ResumeVenue`
//! ([`LinkDeadMan::take_halt`], acted on by `CoreThread::resume_venue`): the same prior states, no
//! other venue and no node-wide state touched, refused while a link of the venue is still down
//! ([`LinkDeadMan::down_links`]) unless forced, in which case the venue's windows start over
//! ([`LinkDeadMan::restart_windows`]).
//!
//! ## A down link outlives idleness (price-only venues)
//!
//! A price-only venue ([`crate::CoreConfig::price_only`]) is armed only while it holds a live order
//! or an open position. An outage disclosed while it holds NOTHING used to be dropped on the floor,
//! and since nothing re-discloses a link that is already down, the first order placed into that
//! outage was unwatched for as long as it lasted. The ledger [`LinkDeadMan::remember_down`] keeps now
//! (and [`LinkDeadMan::check`] parks a still-down window into when its venue goes idle) holds the FACT
//! that the link is down, and nothing more: it cannot trip. The sweep that finds the venue holding
//! something opens a fresh window for each entry, starting at that sweep, so the grace runs from the
//! moment there is something to protect (the old reason for discarding: a window kept from idle
//! time would trip the day something is ordered, on an outage nobody watched, stays true of the
//! window and is not true of this). A `Live` from the same source, or the account being stopped,
//! removes the entry. A mounted venue is never idle, so none of this touches one.
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
use std::time::Duration;

use vike_model::{FeedStatus, TradingState};
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
    /// What a trip does. [`DeadManAction::CancelAllAndHalt`] additionally HALTS THE VENUE whose
    /// link died (every engine of it, in-process, with no sentinel file) — see
    /// [`crate::runtime::CoreThread::sweep_link_deadman`] and the module doc's "The halt is the
    /// VENUE's".
    pub action: DeadManAction,
    /// The venues this switch is ARMED for. A status change on any other venue is ignored
    /// entirely. Folded by the composition root from `vike_venues::link_deadman_default` and the
    /// venues actually mounted; an EMPTY set is a legal, inert configuration (every mounted venue
    /// was session-bounded or discloses no disconnect), and the mount says so per venue.
    pub venues: BTreeSet<String>,
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
    /// trip. The switch re-arms for the next outage; a venue this latch halted is released by the
    /// next sweep once none of ITS links is down ([`LinkDeadMan::take_recovered`]) — the silence
    /// switch's halt, by contrast, never clears by itself.
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
    /// The links disclosed (or known) DOWN while their price-only venue held nothing to protect (the
    /// module doc's "A down link outlives idleness"): a ledger of facts, not windows. Nothing here
    /// can trip; [`Self::check`] opens a window for an entry when its venue starts holding something.
    /// Empty for every core without [`crate::CoreConfig::price_only`], and for the life of a core
    /// whose price-only venues never go down idle.
    dormant: BTreeSet<(String, String, LinkSource)>,
    /// The venue-scoped HALT overlay: venue → engine index → the state that engine held before the
    /// halt (see the module doc's "The halt is the VENUE's"). Empty until a halting trip.
    halted: BTreeMap<String, BTreeMap<usize, TradingState>>,
}

impl LinkDeadMan {
    /// Build from a [`LinkDeadManConfig`]. The grace is clamped to `>= 1ms`.
    pub(crate) fn new(cfg: &LinkDeadManConfig) -> Self {
        LinkDeadMan {
            grace_ms: (cfg.grace.as_millis() as i64).max(1),
            action: cfg.action,
            venues: cfg.venues.clone(),
            links: BTreeMap::new(),
            dormant: BTreeSet::new(),
            halted: BTreeMap::new(),
        }
    }

    /// Note that engine `idx` of `venue` was halted by a trip, holding `prior`. The FIRST prior of
    /// an engine wins: a second trip while it is already halted must not record `Halted` as the
    /// state to return to.
    pub(crate) fn record_halt(&mut self, venue: &str, idx: usize, prior: TradingState) {
        self.halted.entry(venue.to_string()).or_default().entry(idx).or_insert(prior);
    }

    /// Note that `venue` is halted by a trip although the core runs NO engine of it (yet): the entry
    /// has no engines, so [`Self::is_venue_halted`] answers `true` and an account of the venue
    /// hot-started later joins the halt, released with the venue's re-arm like any other.
    pub(crate) fn record_venue_halt(&mut self, venue: &str) {
        self.halted.entry(venue.to_string()).or_default();
    }

    /// Whether `venue` is currently halted by a trip of this latch.
    pub(crate) fn is_venue_halted(&self, venue: &str) -> bool {
        self.halted.contains_key(venue)
    }

    /// The state engine `idx` held before this latch halted it, if it did. The node-wide state of a
    /// core whose primary engine (index 0) is halted for its venue's link alone.
    pub(crate) fn prior_of(&self, idx: usize) -> Option<TradingState> {
        self.halted.values().find_map(|engines| engines.get(&idx).copied())
    }

    /// A node-wide state was raised to `st` (the drawdown latch): a halted engine stays `Halted`,
    /// but the state it returns to must not be looser than the node's.
    pub(crate) fn raise_priors(&mut self, st: TradingState) {
        for prior in self.halted.values_mut().flat_map(|engines| engines.values_mut()) {
            if st.strictness() > prior.strictness() {
                *prior = st;
            }
        }
    }

    /// An ACCOUNT-scoped state `st` asked of engine `idx` while this latch holds it `Halted`: the engine
    /// stays `Halted`, but the state it returns to must not be looser than `st`. `true` when `st` was
    /// stricter than the recorded prior and replaced it, `false` when it changed nothing (a looser or equal
    /// state is dropped: the operator's account-scoped `Active` never reopens a dead venue, `resume-venue`
    /// does). [`Self::raise_priors`]' one-engine twin.
    pub(crate) fn raise_prior_of(&mut self, idx: usize, st: TradingState) -> bool {
        let mut raised = false;
        for prior in self.halted.values_mut().filter_map(|engines| engines.get_mut(&idx)) {
            if st.strictness() > prior.strictness() {
                *prior = st;
                raised = true;
            }
        }
        raised
    }

    /// An account hold on engine `idx` was released while this latch holds it `Halted`: the state it
    /// returns to drops from the hold's own `from` to `to`, and only if that is what the prior is (a prior
    /// something else raised since stays). The hold's raise ([`Self::raise_prior_of`]) undone, no more.
    pub(crate) fn release_prior_of(&mut self, idx: usize, from: TradingState, to: TradingState) {
        for prior in self.halted.values_mut().filter_map(|engines| engines.get_mut(&idx)) {
            if *prior == from && to.strictness() < from.strictness() {
                *prior = to;
            }
        }
    }

    /// Forget every halt: a node-wide stop (the silence dead-man) halted every engine, and nothing may
    /// re-arm over it. The operator's `SetTradingState` goes through [`Self::resume_halts`] instead.
    pub(crate) fn clear_halts(&mut self) {
        self.halted.clear();
    }

    /// A node-wide operator state `st` (`resume` is `Active`): the halt of every venue none of whose
    /// windows is open is forgotten (the caller sets `st` on its engines), but a venue with a window
    /// still open STAYS halted, its recorded priors replaced by `st` (the operator's word on where it
    /// goes when the link is back). Returns each kept venue with its down links, for the caller's log.
    /// The per-venue override is `Command::ResumeVenue` with `force`.
    ///
    /// A link only REMEMBERED down while an idle price-only venue holds nothing ([`Self::remember_down`])
    /// is not a hold: [`Self::take_recovered`] releases such a venue at the next sweep (the module doc's
    /// "discarded as idle is a release"), so keeping it here would log "left halted" for a halt that
    /// ends within one sweep. The three readers agree: a parked link neither holds nor is kept.
    pub(crate) fn resume_halts(&mut self, st: TradingState) -> Vec<(String, Vec<String>)> {
        let down: Vec<(String, Vec<String>)> = self
            .halted
            .keys()
            .map(|venue| (venue.clone(), self.open_links(venue).collect::<Vec<_>>()))
            .filter(|(_, links)| !links.is_empty())
            .collect();
        self.halted.retain(|venue, engines| {
            let kept = down.iter().any(|(v, _)| v == venue);
            if kept {
                engines.values_mut().for_each(|prior| *prior = st);
            }
            kept
        });
        down
    }

    /// Remove and return the halt overlay of ONE venue (the operator's `resume-venue`): the engines
    /// and the state each held before the halt, or `None` when this latch holds no halt of `venue`
    /// (a halt restored from a Snap after a restart is not an overlay).
    pub(crate) fn take_halt(&mut self, venue: &str) -> Option<BTreeMap<usize, TradingState>> {
        self.halted.remove(venue)
    }

    /// Every link of `venue` this latch believes down, one readable cell each: an open window (tripped
    /// or still inside its grace) and a link remembered down across idleness. What the operator's
    /// `resume-venue` refuses on, and says.
    pub(crate) fn down_links(&self, venue: &str) -> Vec<String> {
        let open = self.open_links(venue);
        let parked = self
            .dormant
            .iter()
            .filter(|(v, _, _)| v == venue)
            .map(|(_, symbol, source)| format!("{symbol} [{source:?}, down while idle]"));
        open.chain(parked).collect()
    }

    /// Every OPEN window of `venue` (tripped or still inside its grace), one readable cell each: what
    /// [`Self::take_recovered`] waits on, and so what keeps a halted venue halted.
    fn open_links<'a>(&'a self, venue: &'a str) -> impl Iterator<Item = String> + 'a {
        self.links
            .iter()
            .filter(move |((v, _, _), st)| v == venue && st.down_since.is_some())
            .map(|((_, symbol, source), _)| format!("{symbol} [{source:?}]"))
    }

    /// A forced resume of `venue` with a link still down: every open window of it starts over at
    /// `now_ms` and may trip again, so a link that stays down is acted on after ANOTHER grace instead
    /// of never (a window acts once per outage and the old one had already acted).
    pub(crate) fn restart_windows(&mut self, venue: &str, now_ms: i64) {
        for ((v, _, _), st) in &mut self.links {
            if v == venue && st.down_since.is_some() {
                st.down_since = Some(now_ms);
                st.tripped = false;
            }
        }
    }

    /// Remember that `(venue, symbol, source)` was disclosed `Disconnected` while its price-only venue
    /// held nothing (no window is opened for an idle venue). Armed venues only, like [`Self::observe`].
    pub(crate) fn remember_down(&mut self, venue: &str, symbol: &str, source: LinkSource) {
        if self.venues.contains(venue) {
            self.dormant.insert((venue.to_string(), symbol.to_string(), source));
        }
    }

    /// Remove and return the halted venues none of whose links is down any more, with the engines
    /// and prior states to restore. Empty (no allocation) while nothing is halted.
    pub(crate) fn take_recovered(&mut self) -> Vec<(String, BTreeMap<usize, TradingState>)> {
        if self.halted.is_empty() {
            return Vec::new();
        }
        let up: Vec<String> = self
            .halted
            .keys()
            .filter(|venue| {
                !self.links.iter().any(|((v, _, _), st)| v == *venue && st.down_since.is_some())
            })
            .cloned()
            .collect();
        up.into_iter().filter_map(|v| self.halted.remove(&v).map(|engines| (v, engines))).collect()
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
                let key = (venue.to_string(), symbol.to_string(), source);
                // A `Live` also ends a link remembered down across idleness (`Self::remember_down`).
                self.dormant.remove(&key);
                let Some(st) = self.links.get_mut(&key) else {
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
        let key = (venue.to_string(), symbol.to_string(), LinkSource::TradeLink);
        self.dormant.remove(&key);
        self.links.remove(&key);
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
    /// PARKED, tripped or not (a link still down moves to the dormant ledger, a closed one is simply
    /// dropped), so such a venue neither trips nor carries an old outage's clock into the day
    /// something is ordered. When the venue starts holding something, each link remembered down
    /// ([`Self::remember_down`], or parked here) gets a FRESH window starting at that sweep: the
    /// grace runs from the moment there is something to protect, never from the disclosure, and a
    /// `Live` in between ends the outage. A window that opens while the venue holds something and is
    /// still down at the sweep after the grace trips like any other. `idle` is empty for every other
    /// core, so a mounted venue's windows are never parked and its dormant ledger is always empty.
    /// (No window is OPENED for an idle venue either: `CoreThread::link_window_may_open` gates the
    /// observe, which remembers the disclosure instead.)
    pub(crate) fn check(
        &mut self,
        now_ms: i64,
        idle: &std::collections::BTreeSet<String>,
    ) -> Vec<LinkTrip> {
        if !idle.is_empty() {
            let parked: Vec<(String, String, LinkSource)> = self
                .links
                .iter()
                .filter(|((venue, _, _), st)| idle.contains(venue) && st.down_since.is_some())
                .map(|(key, _)| key.clone())
                .collect();
            self.dormant.extend(parked);
            self.links.retain(|(venue, _, _), _| !idle.contains(venue));
        }
        if !self.dormant.is_empty() {
            let held: Vec<(String, String, LinkSource)> = self
                .dormant
                .iter()
                .filter(|(venue, _, _)| !idle.contains(venue))
                .cloned()
                .collect();
            for key in held {
                self.dormant.remove(&key);
                let st = self.links.entry(key).or_default();
                if st.down_since.is_none() {
                    st.down_since = Some(now_ms);
                }
            }
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

    /// Whether ONE source of `(venue, symbol)` is remembered down across its venue's idleness.
    #[cfg(test)]
    pub(crate) fn is_dormant(&self, venue: &str, symbol: &str, source: LinkSource) -> bool {
        self.dormant.contains(&(venue.to_string(), symbol.to_string(), source))
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
